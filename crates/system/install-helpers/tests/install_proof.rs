//! File-backed checks of the guest's installed-image proof.

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
use std::path::PathBuf;

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
#[allow(
    clippy::too_many_lines,
    reason = "one journey through one command reads as one flow"
)]
fn the_install_proof_is_written_for_matching_identities() {
    {
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

    {
        let mut fixture = Fixture::new();
        fixture.booted("imageDigest", json!(digest('c')));
        fixture.booted("verity", json!("d".repeat(128)));
        let update_origin = format!("code.tilo.so/tilo/tilo:lenovo-83ed-{}", "e".repeat(40));
        fixture.receipt(&json!({"schema": "tilo.installer-offline-target.v1",
            "repository": "code.tilo.so/tilo/tilo", "target_digest": digest('a'),
            "architecture": "aarch64", "hardware_profile": "lenovo-83ed",
            "update_origin": update_origin}));
        fixture.set_origin(&digest('a'), &update_origin);
        let result = fixture.proof().unwrap();
        assert_eq!(result["schema"], "tilo.vm-install-image-proof.v2");
        assert_eq!(result["booted_composefs_digest"], "d".repeat(128));
        assert_eq!(result["installed_composefs_digest"], COMPOSEFS);
        assert_eq!(
            result["selected_target"]["repository"],
            "code.tilo.so/tilo/tilo"
        );
        assert_eq!(
            result["installed_update_origin"],
            format!("ostree-unverified-image:docker://{update_origin}")
        );
        fixture.set_origin(&digest('a'), "registry.tilo.so/tilo/tilo:latest");
        refused(&fixture, "signed offline target");
    }

    {
        let mut fixture = Fixture::new();
        fixture.booted("imageDigest", json!(digest('c')));
        fixture.booted("verity", json!("d".repeat(128)));
        let update_origin = "registry.tilo.so/tilo/tilo-vm:main-1234567-w42-lenovo-83ed";
        fixture.set_origin(&digest('a'), update_origin);
        let mut metadata = json!({"schema": "tilo.installer-dev-target.v1",
            "repository": "registry.tilo.so/tilo/tilo-vm",
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

    {
        let fixture = Fixture::new();
        let receipt = fixture.code_receipt();
        fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        fs::write(
            &receipt,
            json!({"schema": "tilo.installer-code-refresh.v1", "sha256": "9".repeat(64)})
                .to_string(),
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

    {
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
}

#[test]
fn the_install_proof_is_refused() {
    {
        let fixture = Fixture::new();
        fixture.booted("imageDigest", json!(digest('c')));
        refused(&fixture, "development copy proof");
        fixture.booted("imageDigest", json!(digest('a')));
        fixture.set_origin(&digest('c'), "registry.tilo.so/tilo/tilo:latest");
        refused(&fixture, "development copy proof");
    }

    {
        let fixture = Fixture::new();
        *fixture.uki_cmdline.borrow_mut() = format!("composefs={}", "c".repeat(128));
        refused(&fixture, "target UKI differs");
    }

    {
        let fixture = Fixture::new();
        fixture.booted("architecture", json!("amd64"));
        refused(&fixture, "ARM64");
    }

    {
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
}
