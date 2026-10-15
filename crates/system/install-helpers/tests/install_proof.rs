//! File-backed checks of the guest's installed-image proof (ported from
//! `os/tests/test_vm_install_image_proof.py`, which keeps testing the Python
//! seed the installer-code bootstrap puts on old media).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tilo_install_helpers::install_proof::{Host, Request, build_proof, write};
use tilo_install_helpers::{Error, Result};

const COMPOSEFS: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const GUIDS: [&str; 4] = [
    "00000000-0000-4000-8000-000000000000",
    "00000000-0000-4000-8000-000000000001",
    "00000000-0000-4000-8000-000000000002",
    "00000000-0000-4000-8000-000000000003",
];

fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

/// A finished install in a temporary directory, its tools answered from
/// fixtures.
struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    request: Request,
    origin: PathBuf,
    status: RefCell<Value>,
    uki_cmdline: RefCell<String>,
    copied: RefCell<Option<String>>,
    vm_only: Cell<bool>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let root = dir.join("root");
        let deploy_etc = root.join("state/deploy").join(COMPOSEFS).join("etc");
        fs::create_dir_all(&deploy_etc).unwrap();
        let origin = deploy_etc
            .parent()
            .unwrap()
            .join(format!("{COMPOSEFS}.origin"));
        let fixture = Self {
            request: Request {
                target_root: root,
                deploy_etc,
                efi_root: dir.join("efi"),
                target_uki: dir.join("efi/EFI/Linux/target.efi"),
                disk: "/dev/vda".into(),
                efi_partition: "/dev/vda5".into(),
                boot_partition: "/dev/vda6".into(),
                root_partition: "/dev/vda7".into(),
                offline_target_receipt: dir.join("no-receipt.json"),
            },
            _tmp: tmp,
            dir,
            origin,
            status: RefCell::new(json!({"status": {"booted": {"incompatible": false,
                "image": {"imageDigest": digest('a'), "architecture": "arm64"},
                "composefs": {"verity": COMPOSEFS}}}})),
            uki_cmdline: RefCell::new(format!("root=tilo-auto composefs={COMPOSEFS}")),
            copied: RefCell::new(None),
            vm_only: Cell::new(false),
            calls: RefCell::new(Vec::new()),
        };
        fixture.set_origin(&digest('a'), "registry.tilo.so/tilo/tilo:latest");
        fs::create_dir_all(fixture.request.target_uki.parent().unwrap()).unwrap();
        fs::write(&fixture.request.target_uki, b"signed-target-uki").unwrap();
        fixture
    }

    fn set_origin(&self, manifest: &str, update_origin: &str) {
        fs::write(
            &self.origin,
            format!(
                "[image]\nmanifest_digest={manifest}\n[origin]\ncontainer-image-reference=ostree-unverified-image:docker://{update_origin}\n"
            ),
        )
        .unwrap();
    }

    fn booted(&self, key: &str, value: Value) {
        let mut status = self.status.borrow_mut();
        match key {
            "imageDigest" | "architecture" => status["status"]["booted"]["image"][key] = value,
            "verity" => status["status"]["booted"]["composefs"]["verity"] = value,
            _ => status["status"]["booted"][key] = value,
        }
    }

    fn receipt(&mut self, document: &Value) {
        let path = self.dir.join("receipt.json");
        fs::write(&path, document.to_string()).unwrap();
        self.request.offline_target_receipt = path;
    }

    fn proof(&self) -> Result<Value> {
        build_proof(self, &self.request)
    }
}

impl Host for Fixture {
    fn command(&self, argv: &[&str]) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(argv.iter().map(|word| (*word).to_owned()).collect());
        match argv {
            ["bootc", "status", ..] => Ok(self.status.borrow().to_string()),
            ["/usr/libexec/tilo-uki-cmdline", "show", _] => Ok(self.uki_cmdline.borrow().clone()),
            ["blkid", "-p", "-s", "PTUUID", ..] => Ok(GUIDS[0].to_owned()),
            ["blkid", "-s", "PARTUUID", "-o", "value", partition] => Ok(match *partition {
                "/dev/vda5" => GUIDS[1],
                "/dev/vda6" => GUIDS[2],
                "/dev/vda7" => GUIDS[3],
                other => panic!("{other}"),
            }
            .to_owned()),
            ["systemd-detect-virt", "--vm", "--quiet"] => Ok(String::new()),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn dev_vm_marker(&self) -> PathBuf {
        let marker = self.dir.join("vm-marker");
        if self.vm_only.get() {
            fs::write(&marker, "").unwrap();
        }
        marker
    }

    fn proof_port(&self) -> bool {
        self.vm_only.get()
    }

    fn code_receipt(&self) -> PathBuf {
        self.dir.join("code-refresh/active.json")
    }

    fn copied_dev_manifest_digest(&self) -> Result<String> {
        self.copied.borrow().clone().ok_or_else(|| {
            Error::new("development copy proof requires the VM-only installer and proof port")
        })
    }
}

fn refused(fixture: &Fixture, fragment: &str) {
    let error = fixture.proof().unwrap_err();
    assert!(error.0.contains(fragment), "{fragment}: {error}");
}

#[test]
fn matching_booted_origin_uki_and_disk_identities() {
    let fixture = Fixture::new();
    let result = fixture.proof().unwrap();
    assert_eq!(result["schema"], "tilo.vm-install-image-proof.v1");
    assert_eq!(result["architecture"], "arm64");
    assert_eq!(result["booted_image_digest"], digest('a'));
    assert_eq!(result["installed_image_digest"], digest('a'));
    assert_eq!(result["composefs_digest"], COMPOSEFS);
    assert_eq!(result["partuuids"]["root"], GUIDS[3]);
    assert_eq!(result["disk_guid"], GUIDS[0]);
    assert_eq!(
        result["target_uki_sha256"],
        tilo_install_helpers::py::sha256_hex(b"signed-target-uki")
    );
    assert!(result.get("selected_target").is_none());
    assert!(result.get("installer_code_override_sha256").is_none());
}

#[test]
fn distinct_signed_medium_and_target() {
    let mut fixture = Fixture::new();
    fixture.booted("imageDigest", json!(digest('c')));
    fixture.booted("verity", json!("d".repeat(128)));
    let target_ref = format!("code.tilo.so/tilo/tilo@{}", digest('a'));
    let update_origin = format!("code.tilo.so/tilo/tilo:lenovo-83ed-{}", "e".repeat(40));
    fixture.receipt(&json!({"schema": "tilo.installer-offline-target.v1",
        "target_ref": target_ref, "target_digest": digest('a'),
        "hardware_profile": "lenovo-83ed", "update_origin": update_origin}));
    fixture.set_origin(&digest('a'), &update_origin);
    let result = fixture.proof().unwrap();
    assert_eq!(result["schema"], "tilo.vm-install-image-proof.v2");
    assert_eq!(result["booted_composefs_digest"], "d".repeat(128));
    assert_eq!(result["installed_composefs_digest"], COMPOSEFS);
    assert_eq!(result["selected_target"]["target_ref"], target_ref);
    assert_eq!(
        result["installed_update_origin"],
        format!("ostree-unverified-image:docker://{update_origin}")
    );
    fixture.set_origin(&digest('a'), "registry.tilo.so/tilo/tilo:latest");
    refused(&fixture, "signed offline target");
}

#[test]
fn selected_development_target_is_explicit_vm_proof() {
    let mut fixture = Fixture::new();
    fixture.booted("imageDigest", json!(digest('c')));
    fixture.booted("verity", json!("d".repeat(128)));
    let update_origin = "registry.tilo.so/tilo/tilo-vm:main-1234567-w42-lenovo-83ed";
    fixture.set_origin(&digest('a'), update_origin);
    let mut metadata = json!({"schema": "tilo.installer-dev-target.v1",
        "target_ref": format!("registry.tilo.so/tilo/tilo-vm@{}", digest('a')),
        "target_digest": digest('a'), "architecture": "aarch64",
        "hardware_profile": "lenovo-83ed", "update_origin": update_origin,
        "source_revision": "e".repeat(40), "ci_run_id": "42",
        "verifier_key_sha256": null,
        "provenance": {"source_revision": "e".repeat(40), "invocation_id": "42"}});
    fixture.receipt(&metadata);
    // Outside the VM-only development installer it is refused.
    refused(&fixture, "VM-only development installer");
    fixture.vm_only.set(true);
    let result = fixture.proof().unwrap();
    assert_eq!(result["schema"], "tilo.vm-install-image-proof.v4");
    assert_eq!(result["installed_image_digest"], digest('a'));
    assert_eq!(result["booted_composefs_digest"], "d".repeat(128));
    assert_eq!(
        result["selected_target"]["verifier_key_sha256"],
        Value::Null
    );
    assert!(result["selected_target"].get("source_revision").is_none());
    assert!(result["selected_target"].get("ci_run_id").is_none());
    assert!(
        fixture
            .calls
            .borrow()
            .iter()
            .any(|argv| argv == &["systemd-detect-virt", "--vm", "--quiet"])
    );
    metadata["provenance"]["source_revision"] = json!("f".repeat(40));
    fixture.receipt(&metadata);
    refused(&fixture, "source differs");
    metadata["provenance"]["source_revision"] = json!("e".repeat(40));
    metadata["verifier_key_sha256"] = json!("f".repeat(64));
    fixture.receipt(&metadata);
    refused(&fixture, "VM-only");
}

#[test]
fn development_vm_copy_changes_oci_manifest_only() {
    let fixture = Fixture::new();
    fixture.booted("imageDigest", json!(digest('c')));
    *fixture.copied.borrow_mut() = Some(digest('a'));
    let result = fixture.proof().unwrap();
    assert_eq!(result["schema"], "tilo.vm-install-image-proof.v3");
    assert_eq!(result["copied_manifest_digest"], digest('a'));
    *fixture.copied.borrow_mut() = Some(digest('d'));
    refused(&fixture, "VM-local bootc copy");
}

#[test]
fn mismatching_booted_or_installed_image_refused() {
    let fixture = Fixture::new();
    fixture.booted("imageDigest", json!(digest('c')));
    refused(&fixture, "development copy proof");
    fixture.booted("imageDigest", json!(digest('a')));
    fixture.set_origin(&digest('c'), "registry.tilo.so/tilo/tilo:latest");
    refused(&fixture, "development copy proof");
}

#[test]
fn wrong_uki_composefs_or_deployment_path_refused() {
    let mut fixture = Fixture::new();
    *fixture.uki_cmdline.borrow_mut() = format!("composefs={}", "c".repeat(128));
    refused(&fixture, "target UKI differs");
    *fixture.uki_cmdline.borrow_mut() = format!("composefs={COMPOSEFS} composefs={COMPOSEFS}");
    refused(&fixture, "no unique composefs digest");
    *fixture.uki_cmdline.borrow_mut() = format!("composefs={COMPOSEFS}");
    let other = fixture.dir.join("other");
    fs::create_dir(&other).unwrap();
    fixture.request.deploy_etc = other;
    assert!(fixture.proof().is_err());
}

#[test]
fn non_arm64_booted_image_refused() {
    let fixture = Fixture::new();
    fixture.booted("architecture", json!("amd64"));
    refused(&fixture, "ARM64");
}

#[test]
fn incompatible_or_linked_inputs_refused() {
    let fixture = Fixture::new();
    fixture.booted("incompatible", json!(true));
    refused(&fixture, "booted installer image is incompatible");
    let fixture = Fixture::new();
    let real = fixture.dir.join("efi/EFI/Linux/real.efi");
    fs::rename(&fixture.request.target_uki, &real).unwrap();
    std::os::unix::fs::symlink(&real, &fixture.request.target_uki).unwrap();
    refused(&fixture, "installed target UKI is a symlink");
    let mut fixture = Fixture::new();
    let outside = fixture.dir.join("outside.efi");
    fs::write(&outside, b"x").unwrap();
    fixture.request.target_uki = outside;
    refused(&fixture, "outside the selected ESP");
}

#[test]
fn installer_code_refresh_evidence_is_recorded() {
    let fixture = Fixture::new();
    let receipt = fixture.code_receipt();
    fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    fs::write(
        &receipt,
        json!({"schema": "tilo.installer-code-refresh.v1", "sha256": "9".repeat(64)}).to_string(),
    )
    .unwrap();
    let result = fixture.proof().unwrap();
    assert_eq!(result["installer_code_override_sha256"], "9".repeat(64));
    fs::write(
        &receipt,
        json!({"schema": "other", "sha256": "9".repeat(64)}).to_string(),
    )
    .unwrap();
    refused(&fixture, "invalid installer code refresh evidence");
}

#[test]
fn the_written_proof_is_canonical_and_private() {
    let fixture = Fixture::new();
    let output = fixture.dir.join("vm-install-proof.json");
    write(&output, &fixture.proof().unwrap()).unwrap();
    let text = fs::read_to_string(&output).unwrap();
    assert!(text.starts_with("{\"architecture\":\"arm64\",\"booted_composefs_digest\":"));
    assert!(text.ends_with("}\n"));
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&output).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
    assert!(!fixture.dir.join("vm-install-proof.tmp").exists());
    assert!(write(&fixture.dir.join("missing/proof.json"), &json!({})).is_err());
}

#[test]
fn backend_emits_proof_only_after_encrypted_finish() {
    let backend = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/usr/libexec/tilo-install"),
    )
    .unwrap();
    let prepared = backend
        .find("VM_INSTALL_PROOF=/run/tilo-installer/vm-install-proof.json")
        .unwrap();
    let finished = backend
        .find("\"$DISK_ENCRYPT\" finish \"$ENC_LUKS\"")
        .unwrap();
    let emitted = backend
        .find("proof_line=\"TILO_VM_INSTALL_PROOF_${proof_version}=")
        .unwrap();
    assert!(prepared < finished && finished < emitted);
    assert!(backend.contains("VM_INSTALL_PROOF_PORT=/dev/virtio-ports/org.tilo.vm-install-proof"));
    assert!(backend.contains("systemd-detect-virt --vm --quiet"));
    assert!(backend.contains("printf '%s\\n' \"$proof_line\" > \"$VM_INSTALL_PROOF_PORT\""));
    assert!(backend.contains("  /usr/libexec/tilo-vm-install-proof \\\n"));
    assert!(!backend.contains("tilo-vm-install-proof.py"));
}

#[test]
fn usage_errors_exit_2() {
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-vm-install-proof"))
        .args(["--target-root", "/x"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("usage: tilo-vm-install-proof [-h]"),
        "{stderr}"
    );
    assert!(stderr.ends_with(
        "error: the following arguments are required: --deploy-etc, --efi-root, --target-uki, \
         --disk, --efi-partition, --boot-partition, --root-partition, --output\n"
    ));
    let ambiguous = Command::new(env!("CARGO_BIN_EXE_tilo-vm-install-proof"))
        .args(["--efi", "/x"])
        .output()
        .unwrap();
    assert_eq!(ambiguous.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr)
            .contains("ambiguous option: --efi could match --efi-root, --efi-partition")
    );
}
