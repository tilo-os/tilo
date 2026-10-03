//! End-to-end tests for the `tilo-install` trust flags:
//!
//! - `install --require-attestor foo`: refuses when the package
//!   isn't attested by `foo`.
//! - `install --require-attestor foo --allow-self-signed`:
//!   attestor match wins; else the self-signed fallback permits
//!   install.
//! - `install` on a package with a revoked publisher key is
//!   refused unconditionally.
//!
//! The tests build .tapp files in-process (bypassing tilo-tilo-app-package-cli
//! to avoid a circular workspace dep) and drive the built
//! `tilo-install` binary via `std::process::Command` with
//! `XDG_CONFIG_HOME` / `XDG_DATA_HOME` pointed at per-test
//! tempdirs so the tests use isolated configuration.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::{Signer, SigningKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::manifest::TrustAttestation;
use tilo_app_package::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustedAttestor, TrustedAttestors,
    attestation_preimage,
};
use tilo_app_package::{Manifest, SigningIdentity, pack};

/// Path to the compiled `tilo-install` binary. Cargo sets
/// CARGO_BIN_EXE_tilo-install when the [[bin]] block is `tilo-install`.
fn install_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tilo-install"))
}

const SAMPLE_APP_MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "so.tilo.editor"
version     = "1.4.2"
kind        = "app"
runtime     = "tilo-runtime/2025.06"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "tilo-default"
landlock    = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
collab      = "required@1"
tilo-fs      = "required@1"

[entrypoint]
bin             = "bin/editor"
splash-segments = ["bin/editor[0x0:0x300000]", "share/splash.png"]
splash-budget   = 100

[providers]
mime = ["application/vnd.oasis.opendocument.text"]
ext  = ["odt"]
"#;

fn write_payload(dir: &Path) {
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("share")).unwrap();
    std::fs::write(dir.join("bin/editor"), b"#!/notreal").unwrap();
    std::fs::write(dir.join("share/splash.png"), b"PNG\x00\x00\x00").unwrap();
}

fn fresh_key() -> (SigningKey, String) {
    let mut rng = UnwrapErr(SysRng);
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pub_hex)
}

fn manifest(pub_hex: &str) -> Manifest {
    let src = SAMPLE_APP_MANIFEST_TEMPLATE.replace("<HEX>", pub_hex);
    Manifest::from_toml_str(&src).expect("decode")
}

/// Attest and inject the signature into `manifest.trust.attested_by`.
fn attest_and_inject(
    manifest: &mut Manifest,
    payload_dir: &Path,
    publisher_sk: &SigningKey,
    attestor_id: &str,
    attestor_sk: &SigningKey,
) {
    // Payload_root_hash is independent of attestation contents,
    // so a single throwaway pack yields the value we sign over.
    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let signed = pack(payload_dir, manifest, &identity)
        .expect("throwaway pack")
        .meta
        .root;
    let preimage = attestation_preimage(
        &signed.payload_root_hash,
        &publisher_sk.verifying_key().to_bytes(),
    );
    let sig = attestor_sk.sign(&preimage).to_bytes();
    manifest
        .trust
        .attested_by
        .retain(|a| a.attestor != attestor_id);
    manifest.trust.attested_by.push(TrustAttestation {
        attestor: attestor_id.into(),
        claim_hex: String::new(),
        signature_hex: hex::encode(sig),
    });
}

/// Build a .tapp file with the same layout that tilo-tilo-app-package-cli writes.
fn build_tapp(
    tapp_path: &Path,
    manifest: &Manifest,
    publisher_sk: &SigningKey,
    payload_dir: &Path,
) -> tilo_app_package::PackageId {
    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let packed = pack(payload_dir, manifest, &identity).expect("pack");
    let file = std::fs::File::create(tapp_path).unwrap();
    tilo_app_package::tapp::write(file, &packed.meta, Some(&packed)).unwrap();
    packed.id()
}

/// Environment for one test — isolated data + config dirs.
struct TestEnv {
    home: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn xdg_config(&self) -> PathBuf {
        self.home.path().join("config")
    }

    fn xdg_data(&self) -> PathBuf {
        self.home.path().join("data")
    }

    fn tilo_config(&self) -> PathBuf {
        self.xdg_config().join("tilo")
    }

    fn write_trusted_attestors(&self, attestors: &TrustedAttestors) {
        std::fs::create_dir_all(self.tilo_config()).unwrap();
        let path = self.tilo_config().join("trusted-attestors.toml");
        attestors.save(&path).unwrap();
    }

    fn write_revocation(&self, list: &RevocationList) {
        std::fs::create_dir_all(self.tilo_config()).unwrap();
        let path = self.tilo_config().join("revoked-keys.toml");
        list.save(&path).unwrap();
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(install_bin());
        // Wipe every ambient env var — we do NOT want the user's
        // XDG dirs or HOME to leak in.
        c.env_clear();
        c.env("PATH", std::env::var("PATH").unwrap_or_default());
        c.env("XDG_CONFIG_HOME", self.xdg_config());
        c.env("XDG_DATA_HOME", self.xdg_data());
        c.env("HOME", self.home.path()); // fallback for xdg_paths::data_dir
        c
    }
}

fn run(mut cmd: Command) -> (bool, String, String) {
    let out = cmd.output().expect("spawn tilo-install");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), stdout, stderr)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn required_attestors_gate_installs_with_optional_self_signed_fallback() {
    // Phase `install_require_attestor_refuses_self_signed`.
    {
        let env = TestEnv::new();
        let (publisher_sk, publisher_pub_hex) = fresh_key();
        let manifest = manifest(&publisher_pub_hex);

        let payload_dir = tempfile::tempdir().unwrap();
        write_payload(payload_dir.path());

        let tapp_path = env.home.path().join("app.tapp");
        build_tapp(&tapp_path, &manifest, &publisher_sk, payload_dir.path());

        // No attestor registered.
        env.write_trusted_attestors(&TrustedAttestors::default());
        env.write_revocation(&RevocationList::default());

        let mut cmd = env.cmd();
        // These prove *signature* policy. The fixture package declares a
        // runtime bundle it does not ship, which is a separate axis and
        // would otherwise refuse the install before the trust decision
        // could be observed.
        cmd.arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime")
            .arg("--require-attestor")
            .arg("tilo-curated/2026.07");
        let (ok, _stdout, stderr) = run(cmd);
        assert!(!ok, "expected refusal, stderr: {stderr}");
        assert!(
            stderr.contains("--require-attestor")
                && (stderr.contains("self-signed")
                    || stderr.contains("verified as")
                    || stderr.contains("policy refused")),
            "unexpected stderr: {stderr}"
        );
    }
    // Phase `install_require_attestor_permits_matching_attestation`.
    {
        let env = TestEnv::new();
        let (publisher_sk, publisher_pub_hex) = fresh_key();
        let (attestor_sk, attestor_pub_hex) = fresh_key();

        let mut manifest = manifest(&publisher_pub_hex);
        let payload_dir = tempfile::tempdir().unwrap();
        write_payload(payload_dir.path());
        attest_and_inject(
            &mut manifest,
            payload_dir.path(),
            &publisher_sk,
            "tilo-curated/2026.07",
            &attestor_sk,
        );

        let tapp_path = env.home.path().join("app.tapp");
        build_tapp(&tapp_path, &manifest, &publisher_sk, payload_dir.path());

        env.write_trusted_attestors(&TrustedAttestors {
            attestor: vec![TrustedAttestor {
                id: "tilo-curated/2026.07".into(),
                pubkey_hex: attestor_pub_hex,
                scope: AttestorScope::All,
            }],
        });
        env.write_revocation(&RevocationList::default());

        let mut cmd = env.cmd();
        // These prove *signature* policy. The fixture package declares a
        // runtime bundle it does not ship, which is a separate axis and
        // would otherwise refuse the install before the trust decision
        // could be observed.
        cmd.arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime")
            .arg("--require-attestor")
            .arg("tilo-curated/2026.07");
        let (ok, stdout, stderr) = run(cmd);
        assert!(ok, "expected success, stdout={stdout} stderr={stderr}");
        assert!(stdout.contains("installed:"), "stdout: {stdout}");
        assert!(stdout.contains("attested"), "stdout: {stdout}");
    }
    // Phase `install_require_attestor_and_allow_self_signed_takes_attestation_when_present`.
    {
        let env = TestEnv::new();
        let (publisher_sk, publisher_pub_hex) = fresh_key();
        let (attestor_sk, attestor_pub_hex) = fresh_key();

        let mut manifest = manifest(&publisher_pub_hex);
        let payload_dir = tempfile::tempdir().unwrap();
        write_payload(payload_dir.path());
        attest_and_inject(
            &mut manifest,
            payload_dir.path(),
            &publisher_sk,
            "tilo-curated/2026.07",
            &attestor_sk,
        );

        let tapp_path = env.home.path().join("app.tapp");
        build_tapp(&tapp_path, &manifest, &publisher_sk, payload_dir.path());

        env.write_trusted_attestors(&TrustedAttestors {
            attestor: vec![TrustedAttestor {
                id: "tilo-curated/2026.07".into(),
                pubkey_hex: attestor_pub_hex,
                scope: AttestorScope::All,
            }],
        });
        env.write_revocation(&RevocationList::default());

        let mut cmd = env.cmd();
        // These prove *signature* policy. The fixture package declares a
        // runtime bundle it does not ship, which is a separate axis and
        // would otherwise refuse the install before the trust decision
        // could be observed.
        cmd.arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime")
            .arg("--require-attestor")
            .arg("tilo-curated/2026.07")
            .arg("--allow-self-signed");
        let (ok, stdout, stderr) = run(cmd);
        assert!(ok, "expected success, stdout={stdout} stderr={stderr}");
        // Attestor path took priority — the printed "attested" line
        // proves it wasn't the self-signed fallback.
        assert!(stdout.contains("attested"), "stdout: {stdout}");
    }
    // Phase `install_require_attestor_and_allow_self_signed_falls_back_when_no_match`.
    {
        let env = TestEnv::new();
        let (publisher_sk, publisher_pub_hex) = fresh_key();
        let manifest = manifest(&publisher_pub_hex);

        let payload_dir = tempfile::tempdir().unwrap();
        write_payload(payload_dir.path());

        let tapp_path = env.home.path().join("app.tapp");
        build_tapp(&tapp_path, &manifest, &publisher_sk, payload_dir.path());

        // Registry empty — no attestation on file.
        env.write_trusted_attestors(&TrustedAttestors::default());
        env.write_revocation(&RevocationList::default());

        let mut cmd = env.cmd();
        // These prove *signature* policy. The fixture package declares a
        // runtime bundle it does not ship, which is a separate axis and
        // would otherwise refuse the install before the trust decision
        // could be observed.
        cmd.arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime")
            .arg("--require-attestor")
            .arg("tilo-curated/2026.07")
            .arg("--allow-self-signed");
        let (ok, stdout, stderr) = run(cmd);
        assert!(
            ok,
            "expected success (allow-self-signed fallback), stderr: {stderr}"
        );
        assert!(stdout.contains("installed:"), "stdout: {stdout}");
        assert!(
            stderr.contains("--allow-self-signed permits") || stderr.contains("unmatched"),
            "stderr: {stderr}"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn publisher_trust_refuses_revoked_warns_on_self_signed_and_binds_registry_keys() {
    // Phase `install_refuses_revoked_publisher`.
    {
        let env = TestEnv::new();
        let (publisher_sk, publisher_pub_hex) = fresh_key();
        let manifest = manifest(&publisher_pub_hex);

        let payload_dir = tempfile::tempdir().unwrap();
        write_payload(payload_dir.path());

        let tapp_path = env.home.path().join("app.tapp");
        build_tapp(&tapp_path, &manifest, &publisher_sk, payload_dir.path());

        env.write_trusted_attestors(&TrustedAttestors::default());
        env.write_revocation(&RevocationList {
            revoked: vec![RevokedKey {
                pubkey_hex: publisher_pub_hex,
                reason: "compromised 2026-07-05".into(),
            }],
        });

        let mut cmd = env.cmd();
        // These prove *signature* policy. The fixture package declares a
        // runtime bundle it does not ship, which is a separate axis and
        // would otherwise refuse the install before the trust decision
        // could be observed.
        cmd.arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime");
        let (ok, _stdout, stderr) = run(cmd);
        assert!(!ok, "expected refusal, stderr: {stderr}");
        assert!(
            stderr.contains("revoked") || stderr.contains("REFUSED"),
            "stderr: {stderr}"
        );
    }
    // Phase `install_default_warns_but_installs_self_signed`.
    {
        // Preserves current default behavior: no flags → warn on
        // self-signed and proceed.
        let env = TestEnv::new();
        let (publisher_sk, publisher_pub_hex) = fresh_key();
        let manifest = manifest(&publisher_pub_hex);

        let payload_dir = tempfile::tempdir().unwrap();
        write_payload(payload_dir.path());

        let tapp_path = env.home.path().join("app.tapp");
        build_tapp(&tapp_path, &manifest, &publisher_sk, payload_dir.path());

        env.write_trusted_attestors(&TrustedAttestors::default());
        env.write_revocation(&RevocationList::default());

        let mut cmd = env.cmd();
        // These prove *signature* policy. The fixture package declares a
        // runtime bundle it does not ship, which is a separate axis and
        // would otherwise refuse the install before the trust decision
        // could be observed.
        cmd.arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime");
        let (ok, stdout, stderr) = run(cmd);
        assert!(ok, "expected success, stderr: {stderr}");
        assert!(stdout.contains("installed:"), "stdout: {stdout}");
        assert!(
            stderr.contains("self-signed") || stderr.contains("WARN"),
            "stderr: {stderr}"
        );
    }
    // Phase `install_require_publisher_binds_the_package_to_the_registry_key`.
    {
        let env = TestEnv::new();
        let (publisher_sk, publisher_pub_hex) = fresh_key();
        let manifest = manifest(&publisher_pub_hex);
        let payload_dir = tempfile::tempdir().unwrap();
        write_payload(payload_dir.path());
        let tapp_path = env.home.path().join("app.tapp");
        let package_root = build_tapp(&tapp_path, &manifest, &publisher_sk, payload_dir.path());
        env.write_trusted_attestors(&TrustedAttestors::default());
        env.write_revocation(&RevocationList::default());

        let mut mismatched = env.cmd();
        mismatched
            .arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime")
            .arg("--require-publisher")
            .arg("11".repeat(32));
        let (ok, _stdout, stderr) = run(mismatched);
        assert!(!ok, "a different publisher key must be refused");
        assert!(
            stderr.contains("does not match --require-publisher"),
            "{stderr}"
        );

        let mut wrong_root = env.cmd();
        wrong_root
            .arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime")
            .arg("--require-publisher")
            .arg(&publisher_pub_hex)
            .arg("--require-package-root")
            .arg("22".repeat(32));
        let (ok, _stdout, stderr) = run(wrong_root);
        assert!(!ok, "a different signed root must be refused");
        assert!(
            stderr.contains("does not match --require-package-root"),
            "{stderr}"
        );

        let mut matching = env.cmd();
        matching
            .arg("install")
            .arg(&tapp_path)
            .arg("--ignore-missing-runtime")
            .arg("--require-publisher")
            .arg(&publisher_pub_hex)
            .arg("--require-package-root")
            .arg(package_root.to_hex());
        let (ok, stdout, stderr) = run(matching);
        assert!(ok, "matching publisher should install: {stderr}");
        assert!(stdout.contains("installed:"));
    }
}

#[test]
fn trust_store_commands_round_trip() {
    // Phase `trust_add_list_remove_roundtrip`.
    {
        let env = TestEnv::new();
        let pubkey = "aa".repeat(32);

        let mut cmd = env.cmd();
        cmd.arg("trust")
            .arg("add")
            .arg("--id")
            .arg("acme/1")
            .arg("--pubkey")
            .arg(&pubkey);
        let (ok, stdout, stderr) = run(cmd);
        assert!(ok, "trust add failed: {stderr}");
        assert!(stdout.contains("acme/1"), "stdout: {stdout}");

        let mut cmd = env.cmd();
        cmd.arg("trust").arg("list");
        let (ok, stdout, _stderr) = run(cmd);
        assert!(ok);
        assert!(stdout.contains("acme/1"), "list stdout: {stdout}");
        assert!(
            stdout.contains(&pubkey),
            "list stdout missing key: {stdout}"
        );

        let mut cmd = env.cmd();
        cmd.arg("trust").arg("remove").arg("acme/1");
        let (ok, _stdout, stderr) = run(cmd);
        assert!(ok, "trust remove failed: {stderr}");

        let mut cmd = env.cmd();
        cmd.arg("trust").arg("list");
        let (ok, stdout, _stderr) = run(cmd);
        assert!(ok);
        assert!(
            stdout.contains("(no trusted attestors)"),
            "stdout: {stdout}"
        );
    }
    // Phase `revoke_and_revoked_list_roundtrip`.
    {
        let env = TestEnv::new();
        let pubkey = "bb".repeat(32);

        let mut cmd = env.cmd();
        cmd.arg("revoke")
            .arg(&pubkey)
            .arg("--reason")
            .arg("compromised");
        let (ok, stdout, stderr) = run(cmd);
        assert!(ok, "revoke failed: {stderr}");
        assert!(stdout.contains(&pubkey), "stdout: {stdout}");

        let mut cmd = env.cmd();
        cmd.arg("revoked").arg("list");
        let (ok, stdout, _stderr) = run(cmd);
        assert!(ok);
        assert!(stdout.contains(&pubkey), "list stdout: {stdout}");
        assert!(stdout.contains("compromised"), "list stdout: {stdout}");
    }
}
