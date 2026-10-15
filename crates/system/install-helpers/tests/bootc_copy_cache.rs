//! The VM retry cache reuses only one exact booted/cached image pair
//! (formerly `os/tests/test_vm_bootc_copy_cache.py`).

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
use tilo_install_helpers::bootc_copy_cache::{Host, can_reuse, current_receipt, enabled};
use tilo_install_helpers::{Error, Result};

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    status: RefCell<Value>,
    inspect: RefCell<Value>,
    manifest: RefCell<Value>,
    /// `None`: the opt-in as the production `enabled` decides it.
    enabled: Cell<Option<bool>>,
    euid: Cell<u32>,
    owned: Cell<Option<bool>>,
    vm: Cell<bool>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        fs::write(
            root.join("cmdline"),
            format!("root=tilo-auto composefs={}\n", "a".repeat(128)),
        )
        .unwrap();
        fs::create_dir(root.join("receipts")).unwrap();
        Self {
            _tmp: tmp,
            root,
            status: RefCell::new(json!({"status": {"booted": {
                "image": {
                    "image": {"image": "localhost/tilo-installer:latest",
                              "transport": "containers-storage"},
                    "imageDigest": format!("sha256:{}", "b".repeat(64)),
                },
                "ostree": {"checksum": "c".repeat(64)},
            }}})),
            inspect: RefCell::new(json!([{"Id": format!("sha256:{}", "d".repeat(64))}])),
            manifest: RefCell::new(json!({"schemaVersion": 2,
                "layers": [{"digest": format!("sha256:{}", "e".repeat(64)), "size": 123}]})),
            enabled: Cell::new(Some(true)),
            euid: Cell::new(0),
            owned: Cell::new(None),
            vm: Cell::new(true),
        }
    }

    fn receipt(&self) -> PathBuf {
        self.root.join("receipts/vm-bootc-copy-cache-v1.json")
    }

    fn save_current(&self) {
        fs::write(
            self.receipt(),
            serde_json::to_string(&current_receipt(self).unwrap()).unwrap(),
        )
        .unwrap();
    }
}

impl Host for Fixture {
    fn marker(&self) -> PathBuf {
        self.root.join("marker")
    }

    fn receipt_dir(&self) -> PathBuf {
        self.root.join("receipts")
    }

    fn cmdline(&self) -> PathBuf {
        self.root.join("cmdline")
    }

    fn euid(&self) -> u32 {
        self.euid.get()
    }

    fn owned_private_file(&self, path: &Path) -> bool {
        // The opt-in fixture answers for the marker; the receipt is owned
        // when it exists, as a root-run installer leaves it.
        match self.enabled.get() {
            Some(enabled) if path == self.marker() => enabled,
            _ => self
                .owned
                .get()
                .unwrap_or_else(|| path == self.receipt() && path.exists()),
        }
    }

    fn run(&self, argv: &[&str]) -> Result<Vec<u8>> {
        let text = match argv {
            ["systemd-detect-virt", "--vm", "--quiet"] => {
                return if self.vm.get() {
                    Ok(Vec::new())
                } else {
                    Err(Error::new("physical host"))
                };
            }
            ["bootc", "status", "--json"] => self.status.borrow().to_string(),
            ["podman", "image", "inspect", ..] => self.inspect.borrow().to_string(),
            ["skopeo", "inspect", "--raw", ..] => self.manifest.borrow().to_string(),
            other => panic!("{other:?}"),
        };
        Ok(text.into_bytes())
    }
}

#[test]
fn exact_exported_image_and_booted_identity_can_be_reused() {
    let fixture = Fixture::new();
    fixture.save_current();
    assert!(can_reuse(&fixture));
    let receipt = current_receipt(&fixture).unwrap();
    assert_eq!(receipt["version"], 1);
    assert_eq!(receipt["booted"]["composefs_digest"], "a".repeat(128));
    assert_eq!(receipt["booted"]["ostree_checksum"], "c".repeat(64));
    assert_eq!(
        receipt["cached"]["manifest_sha256"].as_str().unwrap().len(),
        64
    );
}

#[test]
fn opt_in_absent_copies() {
    let fixture = Fixture::new();
    fixture.save_current();
    fixture.enabled.set(Some(false));
    assert!(!can_reuse(&fixture));
}

#[test]
fn runtime_opt_in_requires_root_owned_marker_and_vm() {
    let fixture = Fixture::new();
    fixture.enabled.set(None);
    fixture.owned.set(Some(true));
    assert!(enabled(&fixture));
    fixture.owned.set(Some(false));
    assert!(!enabled(&fixture));
    fixture.owned.set(Some(true));
    fixture.euid.set(1000);
    assert!(!enabled(&fixture));
    fixture.euid.set(0);
    fixture.vm.set(false);
    assert!(!enabled(&fixture));
}

#[test]
fn identity_changes_force_copy() {
    let missing = Fixture::new();
    assert!(!can_reuse(&missing));

    let digest = Fixture::new();
    digest.save_current();
    digest.status.borrow_mut()["status"]["booted"]["image"]["imageDigest"] =
        json!(format!("sha256:{}", "f".repeat(64)));
    assert!(!can_reuse(&digest));

    let composefs = Fixture::new();
    composefs.save_current();
    fs::write(
        composefs.cmdline(),
        format!("root=tilo-auto composefs={}\n", "f".repeat(128)),
    )
    .unwrap();
    assert!(!can_reuse(&composefs));

    let image_id = Fixture::new();
    image_id.save_current();
    image_id.inspect.borrow_mut()[0]["Id"] = json!(format!("sha256:{}", "f".repeat(64)));
    assert!(!can_reuse(&image_id));

    let layers = Fixture::new();
    layers.save_current();
    layers.manifest.borrow_mut()["layers"][0]["digest"] =
        json!(format!("sha256:{}", "f".repeat(64)));
    assert!(!can_reuse(&layers));

    let malformed = Fixture::new();
    fs::write(malformed.receipt(), "{broken").unwrap();
    assert!(!can_reuse(&malformed));
}

#[test]
fn ambiguous_or_missing_identities_refuse() {
    let fixture = Fixture::new();
    fs::write(
        fixture.cmdline(),
        format!(
            "composefs={} composefs={}\n",
            "a".repeat(128),
            "a".repeat(128)
        ),
    )
    .unwrap();
    assert_eq!(
        current_receipt(&fixture).unwrap_err().0,
        "booted composefs identity is missing or ambiguous"
    );
    let fixture = Fixture::new();
    fixture.manifest.borrow_mut()["layers"] = json!([]);
    assert_eq!(
        current_receipt(&fixture).unwrap_err().0,
        "cached image has no OCI layers"
    );
    let fixture = Fixture::new();
    fixture.inspect.borrow_mut()[0]["Id"] = json!("d".repeat(64));
    current_receipt(&fixture).unwrap();
    fixture.inspect.borrow_mut()[0]["Id"] = json!("d".repeat(63));
    assert_eq!(
        current_receipt(&fixture).unwrap_err().0,
        "cached image has no SHA-256 ID"
    );
}

#[test]
fn a_bare_ostree_absence_records_null() {
    let fixture = Fixture::new();
    fixture.status.borrow_mut()["status"]["booted"]
        .as_object_mut()
        .unwrap()
        .remove("ostree");
    let receipt = current_receipt(&fixture).unwrap();
    assert_eq!(receipt["booted"]["ostree_checksum"], Value::Null);
}

#[test]
fn record_refuses_without_the_opt_in_and_usage_is_two() {
    let binary = env!("CARGO_BIN_EXE_tilo-vm-bootc-copy-cache");
    let usage = Command::new(binary).output().unwrap();
    assert_eq!(usage.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&usage.stderr),
        "usage: tilo-vm-bootc-copy-cache check|record\n"
    );
    let extra = Command::new(binary).args(["check", "x"]).output().unwrap();
    assert_eq!(extra.status.code(), Some(2));
    // Outside an opted-in VM, a check copies and a record says why it cannot.
    let check = Command::new(binary).arg("check").output().unwrap();
    assert_eq!(check.status.code(), Some(1));
    let record = Command::new(binary).arg("record").output().unwrap();
    assert_eq!(record.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&record.stderr),
        "VM copy cache receipt unavailable: VM cache opt-in is absent\n"
    );
}

#[test]
fn the_installer_runs_the_compiled_cache() {
    let backend = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/usr/libexec/tilo-install"),
    )
    .unwrap();
    assert!(backend.contains("VM_COPY_CACHE=/usr/libexec/tilo-vm-bootc-copy-cache\n"));
    assert!(backend.contains("&& \"$VM_COPY_CACHE\" check; then"));
    assert!(backend.contains("\"$VM_COPY_CACHE\" record \\"));
    assert!(!backend.contains("tilo-vm-bootc-copy-cache.py"));
}
