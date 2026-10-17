//! The development code-refresh probe's cached-target check, on a real
//! boot-input fixture archive with the device, the validator and the
//! promotion observed rather than reached.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tilo_install_helpers::code_refresh_probe::{
    Fixture, fixture_probe, non_cmdline_bytes, promotion_probe,
};
use tilo_install_helpers::dev_target_uki::{Inputs, Selected};
use tilo_install_helpers::py::sha256_hex;
use tilo_install_helpers::{Error, Result};

/// A PE image with one `.cmdline` section holding `cmdline` at offset 256.
fn pe(cmdline: &[u8]) -> Vec<u8> {
    let mut raw = vec![0u8; 512];
    raw[..2].copy_from_slice(b"MZ");
    raw[60..64].copy_from_slice(&64u32.to_le_bytes());
    raw[64..68].copy_from_slice(b"PE\0\0");
    raw[70..72].copy_from_slice(&1u16.to_le_bytes());
    raw[88..96].copy_from_slice(b".cmdline");
    raw[96..100].copy_from_slice(&u32::try_from(cmdline.len()).unwrap().to_le_bytes());
    raw[104..108].copy_from_slice(&64u32.to_le_bytes());
    raw[108..112].copy_from_slice(&256u32.to_le_bytes());
    raw[256..256 + cmdline.len()].copy_from_slice(cmdline);
    raw
}

#[test]
fn the_promotion_receipt_masks_only_the_command_line_and_its_virtual_size() {
    let raw = pe(b"old\0");
    let mut changed = pe(b"new");
    assert_eq!(
        non_cmdline_bytes(&raw).unwrap(),
        non_cmdline_bytes(&changed).unwrap()
    );
    changed[200] = 1;
    assert_ne!(
        non_cmdline_bytes(&raw).unwrap(),
        non_cmdline_bytes(&changed).unwrap()
    );
    changed[108..112].copy_from_slice(&500u32.to_le_bytes());
    assert!(
        non_cmdline_bytes(&changed)
            .unwrap_err()
            .0
            .contains("bounds")
    );
    assert!(non_cmdline_bytes(b"not a pe").is_err());
}

struct Scenario {
    _dir: tempfile::TempDir,
    base: PathBuf,
    archive: PathBuf,
    promoted: Cell<bool>,
    tamper_promotion: bool,
}

impl Fixture for Scenario {
    fn device(&self) -> PathBuf {
        self.archive.clone()
    }

    fn sys_block(&self) -> PathBuf {
        self.base.join("sys")
    }

    fn scratch(&self) -> PathBuf {
        self.base.clone()
    }

    fn is_block_device(&self, _path: &Path) -> bool {
        true
    }

    fn size(&self, path: &Path) -> Result<u64> {
        Ok(fs::metadata(path)?.len())
    }

    fn validate(&self, root: &Path) -> Result<Inputs> {
        if fs::read(root.join("kernel/7/vmlinuz"))? != b"kernel" {
            return Err(Error("fixture kernel differs from expected bytes".into()));
        }
        Ok(Inputs {
            version: "7".into(),
            kernel: root.join("kernel/7/vmlinuz"),
            initrd: root.join("kernel/7/initramfs.img"),
            dtb: None,
        })
    }

    fn promote(
        &self,
        _image_root: &Path,
        _target_root: &Path,
        esp: &Path,
        selected: &Selected,
    ) -> Result<String> {
        self.promoted.set(true);
        if selected.update_origin.as_deref() != Some("registry/test") {
            return Err(Error(
                "fixture update origin differs from registry/test".into(),
            ));
        }
        let digest = "c".repeat(128);
        let uki = esp.join(format!("EFI/Linux/bootc/bootc_composefs-{digest}.efi"));
        let mut bytes = pe(b"root=tilo-auto");
        if self.tamper_promotion {
            bytes[400] = 7;
        }
        fs::write(&uki, bytes)?;
        Ok(digest)
    }

    fn validator(&self) -> PathBuf {
        self.base.join("validator")
    }
}

fn scenario(promotion: Option<Value>, tamper_promotion: bool) -> Scenario {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let files: [(&str, Vec<u8>); 5] = [
        ("kernel/7/vmlinuz", b"kernel".to_vec()),
        ("kernel/7/initramfs.img", b"initrd".to_vec()),
        (
            "usr/lib/tilo/hardware-profile.json",
            br#"{"id":"lenovo-83ed"}"#.to_vec(),
        ),
        ("boot/EFI/Linux/7.efi", pe(b"root=sealed")),
        ("padding", vec![b'x'; 1024 * 1024]),
    ];
    let mut manifest = json!({"schema": "tilo.installer-code-boot-fixture.v1",
        "physical_admission": false, "target_digest": format!("sha256:{}", "a".repeat(64)),
        "files": files.iter().map(|(name, data)| ((*name).to_owned(), Value::String(sha256_hex(data))))
            .collect::<serde_json::Map<_, _>>()});
    if let Some(promotion) = promotion {
        manifest["promotion"] = promotion;
    }
    let archive = base.join("inputs.tar");
    let mut builder = tar::Builder::new(fs::File::create(&archive).unwrap());
    let raw = manifest.to_string();
    let mut append = |name: &str, data: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_size(u64::try_from(data.len()).unwrap());
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
    };
    append("fixture.json", raw.as_bytes());
    for (name, data) in &files {
        append(name, data);
    }
    builder.finish().unwrap();
    drop(builder);
    fs::create_dir_all(base.join("sys/inputs.tar")).unwrap();
    fs::write(base.join("sys/inputs.tar/ro"), "1\n").unwrap();
    fs::write(base.join("validator"), b"compiled validator").unwrap();
    Scenario {
        _dir: dir,
        base,
        archive,
        promoted: Cell::new(false),
        tamper_promotion,
    }
}

fn refused(result: Result<Value>, message: &str) {
    let error: Error = result.unwrap_err();
    assert!(error.0.contains(message), "{error}");
}

#[test]
fn the_original_ci_boot_fixture_validates_and_refuses_tampering() {
    let fixture = scenario(None, false);
    let result = fixture_probe(&fixture).unwrap();
    assert_eq!(
        result["schema"],
        "tilo.installer-code-cached-target-probe.v1"
    );
    assert_eq!(result["profile"], "lenovo-83ed");
    assert_eq!(result["kernel_version"], "7");
    assert_eq!(result["promotion"], Value::Null);
    assert_eq!(result["kernel_sha256"], sha256_hex(b"kernel"));
    assert_eq!(
        result["validator_sha256"],
        sha256_hex(b"compiled validator")
    );
    assert_eq!(
        result["mount"]["uki_fixture"]["sha256"],
        sha256_hex(&fs::read(&fixture.archive).unwrap())
    );
    assert!(
        fs::read_dir(&fixture.base).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("tilo-boot-fixture-")),
        "the scratch copy is removed"
    );

    fs::write(fixture.base.join("sys/inputs.tar/ro"), "0\n").unwrap();
    refused(fixture_probe(&fixture), "writable");
    fs::write(fixture.base.join("sys/inputs.tar/ro"), "1\n").unwrap();
    let data = fs::read(&fixture.archive).unwrap();
    let at = data.windows(6).position(|w| w == b"initrd").unwrap();
    let mut tampered = data;
    tampered[at..at + 6].copy_from_slice(b"BROKEN");
    fs::write(&fixture.archive, tampered).unwrap();
    refused(fixture_probe(&fixture), "differ");
}

fn promotion() -> Value {
    json!({"composefs_digest": "c".repeat(128), "origin": "[origin]\ncontainer-image-reference = x\n",
           "loader": "title Tilo\n", "update_origin": "registry/test"})
}

#[test]
fn a_scratch_promotion_may_change_only_the_command_line() {
    let fixture = scenario(Some(promotion()), false);
    let result = fixture_probe(&fixture).unwrap();
    assert!(fixture.promoted.get());
    assert_eq!(result["promotion"]["composefs_digest"], "c".repeat(128));
    assert_eq!(result["promotion"]["non_cmdline_pe_bytes_preserved"], true);
    assert_eq!(
        result["promotion"]["input_uki_sha256"],
        sha256_hex(&pe(b"root=sealed"))
    );

    let tampering = scenario(Some(promotion()), true);
    refused(fixture_probe(&tampering), "non-command-line PE bytes");
}

#[test]
fn promotion_metadata_is_bounded_before_scratch_writes() {
    let fixture = scenario(None, false);
    let root = fixture.base.join("root");
    fs::create_dir(&root).unwrap();
    let mut long = promotion();
    long["origin"] = json!("x".repeat(16385));
    for metadata in [json!([]), long, json!({"composefs_digest": "short"})] {
        let manifest = json!({"promotion": metadata});
        refused(
            promotion_probe(&fixture, &root, manifest.as_object().unwrap(), "7"),
            "invalid scratch promotion fixture",
        );
        assert!(!root.join("promotion-target").exists());
    }
    assert!(!fixture.promoted.get());
}
